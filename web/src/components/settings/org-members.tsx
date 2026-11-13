import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { Loader2, MoreHorizontal } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { meQuery } from "@/api/queries";
import { orgMembersInit, orgMembersKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DisabledReason } from "@/components/disabled-reason";
import { FormError } from "@/components/form/form-error";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  atLeast,
  canChangeRole,
  canRemoveMember,
  grantableRoles,
  ORG_ROLES,
  type OrgRole,
  roleLabel,
} from "@/lib/roles";

export function OrgMembers({
  org,
  actorRole,
  me,
}: {
  org: string;
  actorRole: OrgRole | undefined;
  me: string;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [actionError, setActionError] = useState<string | undefined>();
  const [removing, setRemoving] = useState<Schema<"OrgMember"> | undefined>();
  const query = $api.useInfiniteQuery("get", "/api/v1/orgs/{org}/members", orgMembersInit(org), {
    pageParamName: "cursor",
    initialPageParam: null,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
  const patch = $api.useMutation("patch", "/api/v1/orgs/{org}/members/{user}", {
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: orgMembersKey(org) });
      void queryClient.invalidateQueries({ queryKey: meQuery.queryKey });
    },
    onError: (error) => {
      const status = problemOf(error)?.status;
      setActionError(
        status === 409
          ? "An organization needs at least one owner. Make someone else an owner first."
          : status === 403
            ? "You can't give that role."
            : problemMessage(error, "Could not change the role."),
      );
    },
  });
  const remove = $api.useMutation("delete", "/api/v1/orgs/{org}/members/{user}");

  if (query.error) {
    throw query.error;
  }

  const rows = query.data?.pages.flatMap((page) => page.data) ?? [];

  function changeRole(member: Schema<"OrgMember">, value: string) {
    const next = ORG_ROLES.find((role) => role === value);
    if (!next || next === member.role) {
      return;
    }
    setActionError(undefined);
    patch.mutate({ params: { path: { org, user: member.user.id } }, body: { role: next } });
  }

  return (
    <div className="flex flex-col gap-3">
      <FormError message={actionError} />
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : (
        <Table aria-label="Members">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Email</TableHead>
              <TableHead>Role</TableHead>
              <TableHead>Joined</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((member) => (
              <MemberRow
                key={member.user.id}
                me={me}
                member={member}
                actorRole={actorRole}
                patchPending={
                  patch.isPending && patch.variables?.params.path.user === member.user.id
                }
                onChangeRole={changeRole}
                onRemove={() => {
                  setRemoving(member);
                }}
              />
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
        open={removing !== undefined}
        onOpenChange={(open) => {
          if (!open) {
            setRemoving(undefined);
            remove.reset();
          }
        }}
        title={removing?.user.id === me ? "Leave organization?" : "Remove member?"}
        description={
          removing?.user.id === me ? (
            "You will lose access to every project in this organization."
          ) : (
            <>
              {removing?.user.name ?? removing?.user.email} loses access to every project in this
              organization.
            </>
          )
        }
        confirmLabel={removing?.user.id === me ? "Leave" : "Remove"}
        destructive
        pending={remove.isPending}
        error={
          remove.error
            ? problemOf(remove.error)?.status === 409
              ? "An organization needs at least one owner. Make someone else an owner first."
              : problemMessage(remove.error, "Could not remove the member.")
            : undefined
        }
        onConfirm={() => {
          if (!removing) {
            return;
          }
          const self = removing.user.id === me;
          remove.mutate(
            { params: { path: { org, user: removing.user.id } } },
            {
              onSuccess: async () => {
                setRemoving(undefined);
                remove.reset();
                if (self) {
                  await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" });
                  void navigate({ to: "/" });
                } else {
                  void queryClient.invalidateQueries({ queryKey: orgMembersKey(org) });
                }
              },
            },
          );
        }}
      />
    </div>
  );
}

function MemberRow({
  me,
  member,
  actorRole,
  patchPending,
  onChangeRole,
  onRemove,
}: {
  me: string;
  member: Schema<"OrgMember">;
  actorRole: OrgRole | undefined;
  patchPending: boolean;
  onChangeRole: (member: Schema<"OrgMember">, value: string) => void;
  onRemove: () => void;
}) {
  const self = member.user.id === me;
  const roleItems = grantableRoles(actorRole);
  const items = roleItems.includes(member.role) ? roleItems : [member.role, ...roleItems];
  const roleReason = !atLeast(actorRole, "admin")
    ? "Only admins and owners can change roles"
    : member.role === "owner" && actorRole !== "owner"
      ? "Only an owner can change an owner's role"
      : undefined;
  const canRemove = canRemoveMember(actorRole, member.role, self);
  const removeReason = !atLeast(actorRole, "admin")
    ? "Only admins and owners can remove members"
    : member.role === "owner" && actorRole !== "owner"
      ? "Only an owner can remove an owner"
      : undefined;

  return (
    <TableRow>
      <TableCell>
        <div className="flex items-center gap-2">
          {member.user.name ?? "—"}
          {self ? <Badge variant="secondary">You</Badge> : null}
        </div>
      </TableCell>
      <TableCell>{member.user.email}</TableCell>
      <TableCell>
        <DisabledReason reason={roleReason}>
          <Select
            value={member.role}
            onValueChange={(value) => {
              onChangeRole(member, value);
            }}
            disabled={!canChangeRole(actorRole, member.role) || patchPending}
          >
            <SelectTrigger aria-label={`Role of ${member.user.email}`} className="h-7 w-28">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {items.map((role) => (
                <SelectItem key={role} value={role}>
                  {roleLabel(role)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </DisabledReason>
      </TableCell>
      <TableCell>
        <TimeText value={member.joined_at} style="datetime" />
      </TableCell>
      <TableCell>
        <DisabledReason reason={canRemove ? undefined : removeReason}>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={`Actions for ${member.user.email}`}
                disabled={!canRemove}
              >
                <MoreHorizontal />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem className="text-destructive" onSelect={onRemove}>
                {self ? "Leave organization" : "Remove from organization"}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </DisabledReason>
      </TableCell>
    </TableRow>
  );
}
