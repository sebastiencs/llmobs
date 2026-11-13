import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Loader2 } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { projectQuery } from "@/api/queries";
import { projectMembersInit, projectMembersKey } from "@/api/queries-settings";
import type { Schema } from "@/api/types";
import { DisabledReason } from "@/components/disabled-reason";
import { FormError } from "@/components/form/form-error";
import { SettingsPage } from "@/components/settings/settings-page";
import { useProject } from "@/components/shell/project-context";
import { Button } from "@/components/ui/button";
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
import { atLeast, canHaveOverride, overrideLabel, roleLabel } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/settings/members")({
  component: ProjectMembersPage,
});

const OVERRIDES = ["admin", "member", "viewer", "none"] as const;

function ProjectMembersPage() {
  const { org, project, ref, data } = useProject();
  const canEdit = atLeast(data.role, "admin");
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const queryClient = useQueryClient();
  const [actionError, setActionError] = useState<string | undefined>();
  const query = $api.useInfiniteQuery(
    "get",
    "/api/v1/projects/{project}/members",
    projectMembersInit(org, project),
    {
      pageParamName: "cursor",
      initialPageParam: null,
      getNextPageParam: (page) => page.next_cursor ?? undefined,
    },
  );

  function onChanged(user: string) {
    void queryClient.invalidateQueries({ queryKey: projectMembersKey(org, project) });
    if (user === me.user.id) {
      void queryClient.invalidateQueries({ queryKey: projectQuery(org, project).queryKey });
    }
  }

  function onFailed(error: unknown) {
    setActionError(
      problemOf(error)?.status === 403
        ? "Only project admins can change access."
        : problemMessage(error, "Could not change access."),
    );
  }

  const put = $api.useMutation("put", "/api/v1/projects/{project}/members/{user}", {
    onSuccess: (_data, variables) => onChanged(variables.params.path.user),
    onError: onFailed,
  });
  const del = $api.useMutation("delete", "/api/v1/projects/{project}/members/{user}", {
    onSuccess: (_data, variables) => onChanged(variables.params.path.user),
    onError: onFailed,
  });

  function changeAccess(member: Schema<"ProjectMember">, value: string) {
    setActionError(undefined);
    const path = { project: ref, user: member.user.id };
    if (value === "inherit") {
      del.mutate({ params: { path } });
      return;
    }
    const role = OVERRIDES.find((item) => item === value);
    if (role) {
      put.mutate({ params: { path }, body: { role } });
    }
  }

  if (query.error) {
    throw query.error;
  }

  const rows = query.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <SettingsPage
      title="Members"
      description="Org members and their access to this project. Org owners and admins always have admin access."
    >
      <FormError message={actionError} />
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : (
        <Table aria-label="Project members">
          <TableHeader>
            <TableRow>
              <TableHead>Member</TableHead>
              <TableHead>Org role</TableHead>
              <TableHead>Project access</TableHead>
              <TableHead>Effective role</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((member) => (
              <TableRow key={member.user.id}>
                <TableCell>
                  <div className="flex flex-col">
                    <span>{member.user.name ?? member.user.email}</span>
                    {member.user.name ? (
                      <span className="text-xs text-muted-foreground">{member.user.email}</span>
                    ) : null}
                  </div>
                </TableCell>
                <TableCell>{roleLabel(member.org_role)}</TableCell>
                <TableCell>
                  {canHaveOverride(member.org_role) ? (
                    <DisabledReason
                      reason={canEdit ? undefined : "Only project admins can change access"}
                    >
                      <Select
                        value={member.override ?? "inherit"}
                        onValueChange={(value) => {
                          changeAccess(member, value);
                        }}
                        disabled={
                          !canEdit ||
                          (put.isPending && put.variables?.params.path.user === member.user.id) ||
                          (del.isPending && del.variables?.params.path.user === member.user.id)
                        }
                      >
                        <SelectTrigger
                          aria-label={`Project access of ${member.user.email}`}
                          className="h-7 w-40"
                        >
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {(["inherit", ...OVERRIDES] as const).map((value) => (
                            <SelectItem key={value} value={value}>
                              {overrideLabel(value)}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </DisabledReason>
                  ) : (
                    <span className="text-muted-foreground">Always admin</span>
                  )}
                </TableCell>
                <TableCell>
                  {member.effective_role ? roleLabel(member.effective_role) : "No access"}
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
    </SettingsPage>
  );
}
