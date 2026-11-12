import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Star, Users } from "lucide-react";
import { useState } from "react";
import { api } from "@/api/client";
import { problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { meQuery, savedViewsQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { sameViewState, searchFromViewState, viewStateOf } from "@/lib/search/common";

type DialogName = "save-as" | "rename" | "delete";

export function SavedViewsMenu({
  kind,
  search,
  onApply,
}: {
  kind: Schema<"SavedViewKind">;
  search: object;
  onApply: (search: Record<string, unknown>) => void;
}) {
  const { org, project } = useProject();
  const queryClient = useQueryClient();
  const views = useQuery(savedViewsQuery(org, project, kind));
  const me = useQuery(meQuery);
  const [dialog, setDialog] = useState<DialogName | null>(null);
  const [name, setName] = useState("");
  const [shared, setShared] = useState(false);
  const [makeDefault, setMakeDefault] = useState(false);
  const [problem, setProblem] = useState<string | undefined>();

  const record = search as Record<string, unknown>;
  const viewId = typeof record.view === "string" ? record.view : undefined;
  const current = views.data?.data.find((view) => view.id === viewId);
  const owned = current !== undefined && current.owner_user_id === me.data?.user.id;
  const modified =
    current !== undefined && !sameViewState(viewStateOf(search), viewStateOf(current.state));
  const label = current?.name ?? "Default view";

  async function refresh(): Promise<void> {
    await queryClient.invalidateQueries({
      queryKey: savedViewsQuery(org, project, kind).queryKey,
    });
  }

  async function patch(
    id: string,
    body: { name?: string; state?: Schema<"UpdateSavedView">["state"]; is_default?: boolean },
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

  function openSaveAs(): void {
    setName("");
    setShared(false);
    setMakeDefault(false);
    setProblem(undefined);
    setDialog("save-as");
  }

  function openRename(): void {
    setName(current?.name ?? "");
    setProblem(undefined);
    setDialog("rename");
  }

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button type="button" variant="outline" size="xs" className="gap-1.5">
            {label}
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
          {views.data?.data.map((view) => (
            <DropdownMenuItem
              key={view.id}
              onSelect={() => onApply({ ...searchFromViewState(view.state), view: view.id })}
            >
              <span className="min-w-0 flex-1 truncate">{view.name}</span>
              {view.shared ? (
                <Users role="img" aria-hidden={false} aria-label="Shared" className="size-3" />
              ) : null}
              {view.is_default ? (
                <Star role="img" aria-hidden={false} aria-label="Default" className="size-3" />
              ) : null}
              {view.id === viewId ? <Check className="size-3" /> : null}
            </DropdownMenuItem>
          ))}
          <DropdownMenuSeparator />
          {owned && current ? (
            <DropdownMenuItem
              onSelect={(event) => {
                event.preventDefault();
                void patch(current.id, { state: storedState(viewStateOf(search)) });
              }}
            >
              Save
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem onSelect={openSaveAs}>Save as…</DropdownMenuItem>
          {owned && current ? (
            <DropdownMenuItem onSelect={openRename}>Rename…</DropdownMenuItem>
          ) : null}
          {owned && current ? (
            <DropdownMenuItem
              onSelect={() => {
                setProblem(undefined);
                setDialog("delete");
              }}
            >
              Delete
            </DropdownMenuItem>
          ) : null}
          {owned && current && !current.is_default ? (
            <DropdownMenuItem
              onSelect={(event) => {
                event.preventDefault();
                void patch(current.id, { is_default: true });
              }}
            >
              Set as my default
            </DropdownMenuItem>
          ) : null}
          {owned && current?.is_default ? (
            <DropdownMenuItem
              onSelect={(event) => {
                event.preventDefault();
                void patch(current.id, { is_default: false });
              }}
            >
              Unset default
            </DropdownMenuItem>
          ) : null}
          {modified && current ? (
            <DropdownMenuItem
              onSelect={() => onApply({ ...searchFromViewState(current.state), view: current.id })}
            >
              Discard changes
            </DropdownMenuItem>
          ) : null}
        </DropdownMenuContent>
      </DropdownMenu>

      <Dialog
        open={dialog === "save-as"}
        onOpenChange={(open) => setDialog(open ? "save-as" : null)}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Save view</DialogTitle>
            <DialogDescription>Save the current filters and columns as a view.</DialogDescription>
          </DialogHeader>
          <NameField name={name} onName={setName} />
          <Flag id="view-share" label="Share with project" checked={shared} onChecked={setShared} />
          <Flag
            id="view-default"
            label="Make my default"
            checked={makeDefault}
            onChecked={setMakeDefault}
          />
          {problem ? <p className="text-xs text-destructive">{problem}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setDialog(null)}>
              Cancel
            </Button>
            <Button
              type="button"
              disabled={name.trim().length === 0}
              onClick={() => {
                void (async () => {
                  setProblem(undefined);
                  const { data, error } = await api.POST("/api/v1/projects/{project}/saved-views", {
                    params: { path: { project: projectRef(org, project) } },
                    body: {
                      kind,
                      name: name.trim(),
                      state: storedState(viewStateOf(search)),
                      shared,
                      is_default: makeDefault,
                    },
                  });
                  if (error || !data) {
                    setProblem(problemText(error));
                    return;
                  }
                  await refresh();
                  setDialog(null);
                  onApply({ ...searchFromViewState(data.state), view: data.id });
                })();
              }}
            >
              Save view
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={dialog === "rename"} onOpenChange={(open) => setDialog(open ? "rename" : null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Rename view</DialogTitle>
            <DialogDescription>Change the name shown in the views menu.</DialogDescription>
          </DialogHeader>
          <NameField name={name} onName={setName} />
          {problem ? <p className="text-xs text-destructive">{problem}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setDialog(null)}>
              Cancel
            </Button>
            <Button
              type="button"
              disabled={name.trim().length === 0 || !current}
              onClick={() => {
                if (!current) {
                  return;
                }
                void (async () => {
                  const ok = await patch(current.id, { name: name.trim() });
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

      <Dialog open={dialog === "delete"} onOpenChange={(open) => setDialog(open ? "delete" : null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Delete view</DialogTitle>
            <DialogDescription>
              Delete {current?.name ?? "this view"}? This cannot be undone.
            </DialogDescription>
          </DialogHeader>
          {problem ? <p className="text-xs text-destructive">{problem}</p> : null}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setDialog(null)}>
              Cancel
            </Button>
            <Button
              type="button"
              variant="destructive"
              onClick={() => {
                if (!current) {
                  return;
                }
                void (async () => {
                  setProblem(undefined);
                  const { error } = await api.DELETE(
                    "/api/v1/projects/{project}/saved-views/{id}",
                    {
                      params: { path: { project: projectRef(org, project), id: current.id } },
                    },
                  );
                  if (error) {
                    setProblem(problemText(error));
                    return;
                  }
                  await refresh();
                  setDialog(null);
                  const next = { ...(search as Record<string, unknown>) };
                  delete next.view;
                  onApply(next);
                })();
              }}
            >
              Delete view
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function NameField({ name, onName }: { name: string; onName: (name: string) => void }) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor="view-name">Name</Label>
      <Input
        id="view-name"
        value={name}
        className="h-8"
        onChange={(event) => onName(event.target.value)}
      />
    </div>
  );
}

function Flag({
  id,
  label,
  checked,
  onChecked,
}: {
  id: string;
  label: string;
  checked: boolean;
  onChecked: (on: boolean) => void;
}) {
  return (
    <span className="flex items-center gap-2">
      <Switch id={id} size="sm" checked={checked} onCheckedChange={onChecked} />
      <Label htmlFor={id}>{label}</Label>
    </span>
  );
}

function storedState(state: Record<string, string>): Schema<"CreateSavedView">["state"] {
  return state as Schema<"CreateSavedView">["state"];
}

function problemText(error: unknown): string {
  const problem = problemOf(error);
  if (!problem) {
    return "Could not save the view";
  }
  return problem.detail ?? problem.title;
}
