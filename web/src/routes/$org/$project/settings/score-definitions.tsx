import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Gauge, Loader2 } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage } from "@/api/problem";
import { scoreDefinitionsInit, scoreDefinitionsKey } from "@/api/queries-evaluation";
import type { Schema } from "@/api/types";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DisabledReason } from "@/components/disabled-reason";
import { EmptyState } from "@/components/empty-state";
import {
  SCORE_DATA_TYPE_LABELS,
  ScoreDefinitionDialog,
} from "@/components/evaluators/score-definition-dialog";
import { FormError } from "@/components/form/form-error";
import { SettingsPage } from "@/components/settings/settings-page";
import { useProject } from "@/components/shell/project-context";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { canManageEvaluation } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/settings/score-definitions")({
  component: ScoreDefinitionsPage,
});

const NO_PERMISSION = "Only members can manage score definitions";

/** The range of a numeric score, the labels of a categorical one, else a dash. */
function rangeOf(definition: Schema<"ScoreDefinition">): string {
  if (definition.data_type === "categorical") {
    return definition.categories.map((category) => category.label).join(", ") || "—";
  }
  if (definition.data_type !== "numeric") {
    return "—";
  }
  const { min_value: min, max_value: max } = definition;
  if (min !== undefined && max !== undefined) {
    return `${min} – ${max}`;
  }
  if (min !== undefined) {
    return `≥ ${min}`;
  }
  if (max !== undefined) {
    return `≤ ${max}`;
  }
  return "No range";
}

function ScoreDefinitionsPage() {
  const { org, project, ref, data } = useProject();
  const canManage = canManageEvaluation(data.role);
  const queryClient = useQueryClient();
  const [includeArchived, setIncludeArchived] = useState(false);
  const [creating, setCreating] = useState(false);
  const [actionError, setActionError] = useState<string | undefined>();
  // Never cleared on close, so the dialogs do not blank while they animate out.
  const [editing, setEditing] = useState<Schema<"ScoreDefinition"> | undefined>();
  const [editOpen, setEditOpen] = useState(false);
  const [archiveTarget, setArchiveTarget] = useState<Schema<"ScoreDefinition"> | undefined>();
  const [archiveOpen, setArchiveOpen] = useState(false);
  const query = $api.useInfiniteQuery(
    "get",
    "/api/v1/projects/{project}/score-definitions",
    scoreDefinitionsInit(org, project, includeArchived),
    {
      pageParamName: "cursor",
      initialPageParam: null,
      getNextPageParam: (page) => page.next_cursor ?? undefined,
    },
  );
  const archive = $api.useMutation("delete", "/api/v1/projects/{project}/score-definitions/{id}");
  const restore = $api.useMutation("patch", "/api/v1/projects/{project}/score-definitions/{id}");

  function refresh() {
    void queryClient.invalidateQueries({ queryKey: scoreDefinitionsKey(org, project) });
  }

  if (query.error) {
    throw query.error;
  }

  const rows = query.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <SettingsPage
      title="Score definitions"
      description="Named score shapes. Annotations and evaluators must fit them."
      actions={
        <DisabledReason reason={canManage ? undefined : NO_PERMISSION}>
          <Button
            type="button"
            disabled={!canManage}
            onClick={() => {
              setActionError(undefined);
              setCreating(true);
            }}
          >
            Add definition
          </Button>
        </DisabledReason>
      }
    >
      <div className="flex items-center gap-2">
        <Switch
          id="show-archived"
          size="sm"
          checked={includeArchived}
          onCheckedChange={setIncludeArchived}
        />
        <Label htmlFor="show-archived">Show archived</Label>
      </div>
      <FormError message={actionError} />
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : rows.length === 0 ? (
        <EmptyState
          icon={Gauge}
          title="No score definitions yet"
          description="Add one to check score writes and evaluator outputs against its type and range."
        />
      ) : (
        <Table aria-label="Score definitions">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Type</TableHead>
              <TableHead>Range or categories</TableHead>
              <TableHead>Description</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>
                <span className="sr-only">Actions</span>
              </TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((definition) => (
              <TableRow key={definition.id}>
                <TableCell>{definition.name}</TableCell>
                <TableCell>{SCORE_DATA_TYPE_LABELS[definition.data_type]}</TableCell>
                <TableCell>{rangeOf(definition)}</TableCell>
                <TableCell>{definition.description ?? "—"}</TableCell>
                <TableCell>
                  {definition.archived_at ? <Badge variant="secondary">Archived</Badge> : "Live"}
                </TableCell>
                <TableCell>
                  <div className="flex items-center gap-1">
                    <DisabledReason reason={canManage ? undefined : NO_PERMISSION}>
                      <Button
                        type="button"
                        variant="ghost"
                        size="sm"
                        disabled={!canManage}
                        onClick={() => {
                          setActionError(undefined);
                          setEditing(definition);
                          setEditOpen(true);
                        }}
                      >
                        Edit
                      </Button>
                    </DisabledReason>
                    {definition.archived_at ? (
                      <DisabledReason reason={canManage ? undefined : NO_PERMISSION}>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          disabled={!canManage || restore.isPending}
                          onClick={() => {
                            setActionError(undefined);
                            restore.mutate(
                              {
                                params: { path: { project: ref, id: definition.id } },
                                body: { archived: false },
                              },
                              {
                                onSuccess: refresh,
                                onError: (caught) => {
                                  setActionError(
                                    problemMessage(caught, "Could not restore the definition."),
                                  );
                                },
                              },
                            );
                          }}
                        >
                          Restore
                        </Button>
                      </DisabledReason>
                    ) : (
                      <DisabledReason reason={canManage ? undefined : NO_PERMISSION}>
                        <Button
                          type="button"
                          variant="ghost"
                          size="sm"
                          disabled={!canManage}
                          onClick={() => {
                            setActionError(undefined);
                            setArchiveTarget(definition);
                            setArchiveOpen(true);
                          }}
                        >
                          Archive
                        </Button>
                      </DisabledReason>
                    )}
                  </div>
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
      <ScoreDefinitionDialog
        org={org}
        project={project}
        open={creating}
        onOpenChange={setCreating}
      />
      <ScoreDefinitionDialog
        org={org}
        project={project}
        definition={editing}
        open={editOpen}
        onOpenChange={setEditOpen}
      />
      <ConfirmDialog
        open={archiveOpen}
        onOpenChange={(open) => {
          if (!open) {
            setArchiveOpen(false);
            archive.reset();
          }
        }}
        title={`Archive ${archiveTarget?.name ?? "this definition"}?`}
        description="Scores keep their values; new scores are no longer checked against it."
        confirmLabel="Archive"
        pending={archive.isPending}
        error={
          archive.error
            ? problemMessage(archive.error, "Could not archive the definition.")
            : undefined
        }
        onConfirm={() => {
          if (!archiveTarget) {
            return;
          }
          archive.mutate(
            { params: { path: { project: ref, id: archiveTarget.id } } },
            {
              onSuccess: () => {
                setArchiveOpen(false);
                archive.reset();
                refresh();
              },
            },
          );
        }}
      />
    </SettingsPage>
  );
}
