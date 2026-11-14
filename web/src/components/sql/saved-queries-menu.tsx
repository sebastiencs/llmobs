import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Bookmark, Check, Users } from "lucide-react";
import { useState } from "react";
import { api } from "@/api/client";
import { problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { meQuery, savedQueriesQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { DisabledReason } from "@/components/disabled-reason";
import { Flag, NameField } from "@/components/saved-views/saved-views-menu";
import { useProject } from "@/components/shell/project-context";
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
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { atLeast } from "@/lib/roles";
import {
  parseSavedState,
  type SqlSavedState,
  sameSavedState,
  storedState,
} from "@/lib/sql/saved-state";

type DialogName = "save-as" | "rename" | "delete";

export function SavedQueriesMenu({
  viewId,
  current,
  onLoad,
  onSaved,
  onCleared,
}: {
  /** `search.view`. */
  viewId: string | undefined;
  /** What "Save" would store now: `savedStateOf(draft, search)`. */
  current: SqlSavedState;
  /** Load a saved query into the console. */
  onLoad: (view: Schema<"SavedView">) => void;
  /** A query was created from the current draft; the console records its id. */
  onSaved: (view: Schema<"SavedView">) => void;
  /** The loaded query was deleted. */
  onCleared: () => void;
}) {
  const { org, project, data } = useProject();
  const queryClient = useQueryClient();
  const views = useQuery(savedQueriesQuery(org, project));
  const me = useQuery(meQuery);
  const [dialog, setDialog] = useState<DialogName | null>(null);
  const [name, setName] = useState("");
  const [shared, setShared] = useState(false);
  const [problem, setProblem] = useState<string | undefined>();

  const loaded = views.data?.data.find((view) => view.id === viewId);
  const owned =
    loaded !== undefined && me.data !== undefined && loaded.owner_user_id === me.data.user.id;
  const canShare = atLeast(data.role, "member");
  const isAdmin = atLeast(data.role, "admin");
  /** A viewer who owns a shared query cannot change it (D-H-6). */
  const editBlocked = owned && loaded?.shared === true && !canShare;
  const ownerless = loaded !== undefined && loaded.owner_user_id === undefined;
  const modified = loaded !== undefined && !sameSavedState(current, parseSavedState(loaded.state));

  async function refresh(): Promise<void> {
    await queryClient.invalidateQueries({ queryKey: savedQueriesQuery(org, project).queryKey });
  }

  async function patch(
    id: string,
    body: { name?: string; state?: Schema<"UpdateSavedView">["state"]; shared?: boolean },
  ): Promise<boolean> {
    setProblem(undefined);
    const { error } = await api.PATCH("/api/v1/projects/{project}/saved-views/{id}", {
      params: { path: { project: projectRef(org, project), id } },
      body,
    });
    if (error) {
      setProblem(problemText(error));
      return false;
    }
    await refresh();
    return true;
  }

  function openDialog(next: DialogName) {
    setProblem(undefined);
    setName(next === "rename" ? (loaded?.name ?? "") : "");
    setShared(false);
    setDialog(next);
  }

  function closeDialog() {
    setDialog(null);
    setProblem(undefined);
  }

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button type="button" variant="outline" size="xs" className="gap-1.5">
            <Bookmark />
            {loaded?.name ?? "Saved queries"}
            {modified ? (
              <span
                role="status"
                aria-label="Unsaved changes"
                className="size-1.5 shrink-0 rounded-full bg-primary"
              />
            ) : null}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-56">
          {problem ? (
            <p role="alert" className="px-2 py-1 text-xs text-destructive">
              {problem}
            </p>
          ) : null}
          {views.data?.data.length === 0 ? (
            <DropdownMenuItem disabled>No saved queries</DropdownMenuItem>
          ) : null}
          {views.data?.data.map((view) => (
            <DropdownMenuItem key={view.id} onSelect={() => onLoad(view)}>
              <span className="min-w-0 flex-1 truncate">{view.name}</span>
              {view.shared ? (
                <Users role="img" aria-hidden={false} aria-label="Shared" className="size-3" />
              ) : null}
              {view.id === viewId ? <Check className="size-3" /> : null}
            </DropdownMenuItem>
          ))}
          <DropdownMenuSeparator />
          {owned && loaded ? (
            <DropdownMenuItem
              disabled={editBlocked}
              onSelect={(event) => {
                event.preventDefault();
                void patch(loaded.id, { state: storedState(current) });
              }}
            >
              <span className="flex flex-col">
                Save
                {editBlocked ? (
                  <BlockedReason>Changing a shared query needs the member role</BlockedReason>
                ) : null}
              </span>
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem onSelect={() => openDialog("save-as")}>Save as…</DropdownMenuItem>
          {owned && loaded ? (
            <DropdownMenuItem disabled={editBlocked} onSelect={() => openDialog("rename")}>
              <span className="flex flex-col">
                Rename…
                {editBlocked ? (
                  <BlockedReason>Changing a shared query needs the member role</BlockedReason>
                ) : null}
              </span>
            </DropdownMenuItem>
          ) : null}
          {owned && loaded && !loaded.shared ? (
            <DropdownMenuItem
              disabled={!canShare}
              onSelect={(event) => {
                event.preventDefault();
                void patch(loaded.id, { shared: true });
              }}
            >
              <span className="flex flex-col">
                Share with project
                {!canShare ? <BlockedReason>Needs the member role</BlockedReason> : null}
              </span>
            </DropdownMenuItem>
          ) : null}
          {loaded?.shared && !ownerless && (owned || isAdmin) ? (
            <DropdownMenuItem
              onSelect={(event) => {
                event.preventDefault();
                void patch(loaded.id, { shared: false });
              }}
            >
              Stop sharing
            </DropdownMenuItem>
          ) : null}
          {loaded && (owned || (isAdmin && loaded.shared)) ? (
            <DropdownMenuItem onSelect={() => openDialog("delete")}>Delete</DropdownMenuItem>
          ) : null}
          {modified && loaded ? (
            <DropdownMenuItem onSelect={() => onLoad(loaded)}>Discard changes</DropdownMenuItem>
          ) : null}
        </DropdownMenuContent>
      </DropdownMenu>

      <Dialog
        open={dialog === "save-as"}
        onOpenChange={(open) => {
          if (!open) {
            closeDialog();
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Save query</DialogTitle>
            <DialogDescription>
              Save the query, its parameters and the time range.
            </DialogDescription>
          </DialogHeader>
          <NameField name={name} onName={setName} />
          {canShare ? (
            <Flag
              id="query-share"
              label="Share with project"
              checked={shared}
              onChecked={setShared}
            />
          ) : (
            <DisabledReason reason="Sharing needs the member role">
              <span className="flex items-center gap-2">
                <Switch id="query-share" size="sm" checked={false} disabled />
                <Label htmlFor="query-share">Share with project</Label>
              </span>
            </DisabledReason>
          )}
          {problem ? <p className="text-xs text-destructive">{problem}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={closeDialog}>
              Cancel
            </Button>
            <Button
              type="button"
              disabled={name.trim().length === 0}
              onClick={() => {
                void (async () => {
                  setProblem(undefined);
                  const { data: created, error } = await api.POST(
                    "/api/v1/projects/{project}/saved-views",
                    {
                      params: { path: { project: projectRef(org, project) } },
                      body: {
                        kind: "sql",
                        name: name.trim(),
                        state: storedState(current),
                        shared,
                        is_default: false,
                      },
                    },
                  );
                  if (error || !created) {
                    setProblem(problemText(error));
                    return;
                  }
                  await refresh();
                  setDialog(null);
                  onSaved(created);
                })();
              }}
            >
              Save query
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={dialog === "rename"}
        onOpenChange={(open) => {
          if (!open) {
            closeDialog();
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Rename query</DialogTitle>
            <DialogDescription>Change the name shown in the saved queries menu.</DialogDescription>
          </DialogHeader>
          <NameField name={name} onName={setName} />
          {problem ? <p className="text-xs text-destructive">{problem}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={closeDialog}>
              Cancel
            </Button>
            <Button
              type="button"
              disabled={name.trim().length === 0 || !loaded}
              onClick={() => {
                if (!loaded) {
                  return;
                }
                void (async () => {
                  const ok = await patch(loaded.id, { name: name.trim() });
                  if (ok) {
                    setDialog(null);
                  }
                })();
              }}
            >
              Rename
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog
        open={dialog === "delete"}
        onOpenChange={(open) => {
          if (!open) {
            closeDialog();
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete query</DialogTitle>
            <DialogDescription>
              Delete {loaded?.name ?? "this query"}? This cannot be undone.
            </DialogDescription>
          </DialogHeader>
          {problem ? <p className="text-xs text-destructive">{problem}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={closeDialog}>
              Cancel
            </Button>
            <Button
              type="button"
              variant="destructive"
              onClick={() => {
                if (!loaded) {
                  return;
                }
                void (async () => {
                  setProblem(undefined);
                  const { error } = await api.DELETE(
                    "/api/v1/projects/{project}/saved-views/{id}",
                    {
                      params: { path: { project: projectRef(org, project), id: loaded.id } },
                    },
                  );
                  if (error) {
                    setProblem(problemText(error));
                    return;
                  }
                  await refresh();
                  setDialog(null);
                  onCleared();
                })();
              }}
            >
              Delete query
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function BlockedReason({ children }: { children: string }) {
  return <span className="text-muted-foreground text-xs">{children}</span>;
}

function problemText(error: unknown): string {
  const problem = problemOf(error);
  return problem?.detail ?? problem?.title ?? "Could not save the query";
}
