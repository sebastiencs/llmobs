import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Loader2, UserPlus, Users } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { adminUsersInit, adminUsersKey } from "@/api/queries-admin";
import type { Schema } from "@/api/types";
import { AddUserDialog } from "@/components/admin/add-user-dialog";
import { adminUserError } from "@/components/admin/messages";
import { UserActions } from "@/components/admin/user-actions";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { FormError } from "@/components/form/form-error";
import { SecretDialog } from "@/components/secret-dialog";
import { TimeText } from "@/components/time-text";
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

export const Route = createFileRoute("/admin/users")({
  component: AdminUsersPage,
});

function AdminUsersPage() {
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const users = $api.useInfiniteQuery("get", "/api/v1/admin/users", adminUsersInit(), {
    pageParamName: "cursor",
    initialPageParam: null,
    getNextPageParam: (page) => page.next_cursor ?? undefined,
  });
  const patch = $api.useMutation("patch", "/api/v1/admin/users/{id}");
  // gcTime 0: the response holds the reset link, a secret shown once (D-L-5).
  const resetLink = $api.useMutation("post", "/api/v1/admin/users/{id}/reset-link", {
    gcTime: 0,
  });
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | undefined>();
  // Never cleared on close, so the title does not flash "Disable ?" while the dialog animates out.
  const [disableTarget, setDisableTarget] = useState<Schema<"AdminUser"> | undefined>();
  const [disableOpen, setDisableOpen] = useState(false);
  const [disableError, setDisableError] = useState<string | undefined>();
  const [resetFor, setResetFor] = useState<Schema<"AdminUser"> | undefined>();
  const [addOpen, setAddOpen] = useState(false);
  const [created, setCreated] = useState<{ email: string; url: string } | undefined>();

  if (users.error) {
    throw users.error;
  }
  if (users.isPending) {
    return (
      <div className="flex flex-col gap-2">
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-8 w-full" />
        <Skeleton className="h-8 w-full" />
      </div>
    );
  }

  const rows = users.data.pages.flatMap((page) => page.data);
  // One action at a time: the per-call callbacks only run for the latest call (README § Pitfalls).
  const busy = patch.isPending || resetLink.isPending;

  function invalidate() {
    void queryClient.invalidateQueries({ queryKey: adminUsersKey() });
  }

  function onToggleAdmin(user: Schema<"AdminUser">) {
    setActionError(undefined);
    patch.mutate(
      { params: { path: { id: user.id } }, body: { is_instance_admin: !user.is_instance_admin } },
      { onSuccess: invalidate, onError: (error) => setActionError(adminUserError(error)) },
    );
  }

  function onEnable(user: Schema<"AdminUser">) {
    setActionError(undefined);
    patch.mutate(
      { params: { path: { id: user.id } }, body: { disabled: false } },
      { onSuccess: invalidate, onError: (error) => setActionError(adminUserError(error)) },
    );
  }

  function onDisable(user: Schema<"AdminUser">) {
    setActionError(undefined);
    setDisableError(undefined);
    setDisableTarget(user);
    setDisableOpen(true);
  }

  function onResetLink(user: Schema<"AdminUser">) {
    setActionError(undefined);
    setResetFor(user);
    resetLink.mutate(
      { params: { path: { id: user.id } } },
      {
        onError: (error) => {
          setResetFor(undefined);
          setActionError(adminUserError(error));
        },
      },
    );
  }

  return (
    <>
      <div className="flex items-center gap-4">
        <h2 className="text-lg font-medium">Users</h2>
        <div className="flex-1" />
        <Button type="button" onClick={() => setAddOpen(true)}>
          <UserPlus aria-hidden="true" />
          Add user
        </Button>
      </div>
      <FormError message={actionError} />
      {rows.length === 0 ? (
        <EmptyState icon={Users} title="No users" />
      ) : (
        <Table aria-label="Users">
          <TableHeader>
            <TableRow>
              <TableHead>Email</TableHead>
              <TableHead>Name</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Providers</TableHead>
              <TableHead>Created</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((user) => (
              <TableRow key={user.id}>
                <TableCell>
                  {user.email}
                  {user.id === me.user.id ? <Badge variant="outline">You</Badge> : null}
                </TableCell>
                <TableCell>
                  {user.name ? user.name : <span className="text-muted-foreground">—</span>}
                </TableCell>
                <TableCell>
                  {user.is_instance_admin ||
                  user.disabled ||
                  !user.email_verified ||
                  !user.has_password ? (
                    <div className="flex flex-wrap gap-1">
                      {user.is_instance_admin ? <Badge variant="secondary">Admin</Badge> : null}
                      {user.disabled ? <Badge variant="destructive">Disabled</Badge> : null}
                      {!user.email_verified ? <Badge variant="outline">Unverified</Badge> : null}
                      {!user.has_password ? <Badge variant="outline">SSO only</Badge> : null}
                    </div>
                  ) : null}
                </TableCell>
                <TableCell>
                  {user.providers.length > 0 ? (
                    <span className="font-mono text-xs">{user.providers.join(", ")}</span>
                  ) : (
                    "—"
                  )}
                </TableCell>
                <TableCell>
                  <TimeText value={user.created_at} style="datetime" />
                </TableCell>
                <TableCell>
                  <UserActions
                    user={user}
                    self={user.id === me.user.id}
                    pending={busy}
                    onToggleAdmin={onToggleAdmin}
                    onDisable={onDisable}
                    onEnable={onEnable}
                    onResetLink={onResetLink}
                  />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {users.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="w-fit"
          disabled={users.isFetchingNextPage}
          onClick={() => {
            void users.fetchNextPage();
          }}
        >
          {users.isFetchingNextPage ? <Loader2 className="animate-spin" /> : null}
          Load more
        </Button>
      ) : null}
      <ConfirmDialog
        open={disableOpen}
        onOpenChange={(open) => {
          if (!open) {
            setDisableOpen(false);
            setDisableError(undefined);
          }
        }}
        title={`Disable ${disableTarget?.email ?? ""}?`}
        description="They are signed out at once and can't sign in until an admin enables the account. Their personal access tokens stop working."
        confirmLabel="Disable"
        destructive
        pending={patch.isPending}
        error={disableError}
        onConfirm={() => {
          if (!disableTarget) {
            return;
          }
          setDisableError(undefined);
          patch.mutate(
            { params: { path: { id: disableTarget.id } }, body: { disabled: true } },
            {
              onSuccess: () => {
                setDisableOpen(false);
                invalidate();
              },
              onError: (error) => setDisableError(adminUserError(error)),
            },
          );
        }}
      />
      {resetFor && resetLink.data ? (
        <SecretDialog
          open
          title={`Reset link for ${resetFor.email}`}
          secret={resetLink.data.url}
          description="This link sets a new password and works for one hour."
          onDone={() => {
            setResetFor(undefined);
            resetLink.reset();
          }}
        />
      ) : null}
      <AddUserDialog
        open={addOpen}
        onOpenChange={setAddOpen}
        onCreated={(email, url) => {
          setAddOpen(false);
          setCreated({ email, url });
          invalidate();
        }}
      />
      {created ? (
        <SecretDialog
          open
          title="User created"
          secret={created.url}
          description={`Send this link to ${created.email}. It sets their password and works for one hour.`}
          onDone={() => setCreated(undefined)}
        />
      ) : null}
    </>
  );
}
