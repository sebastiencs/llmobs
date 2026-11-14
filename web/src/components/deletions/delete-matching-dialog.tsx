import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { FormError } from "@/components/form/form-error";
import { useProject } from "@/components/shell/project-context";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import { invalidateProjectReads } from "@/lib/deletions";

export type TraceSelection = {
  filter?: string;
  search?: string;
  from: string; // RFC 3339
  to: string; // RFC 3339
  includeEval: boolean;
  includeExperiments: boolean;
};

const CAPPED = "More than 100,000 traces match. Narrow the filter or the time range.";

/** Counts the traces a list selection matches, then deletes them after a confirmation. */
export function DeleteMatchingDialog({
  open,
  onOpenChange,
  selection,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  selection: TraceSelection;
}) {
  // Esc and the outside click must not close the dialog while the deletion request runs.
  const [deleting, setDeleting] = useState(false);
  return (
    <AlertDialog
      open={open}
      onOpenChange={(next) => {
        if (!deleting) {
          onOpenChange(next);
        }
      }}
    >
      <AlertDialogContent>
        <DeleteMatchingBody
          selection={selection}
          onClose={() => {
            onOpenChange(false);
          }}
          onDeleting={setDeleting}
        />
      </AlertDialogContent>
    </AlertDialog>
  );
}

function DeleteMatchingBody({
  selection,
  onClose,
  onDeleting,
}: {
  selection: TraceSelection;
  onClose: () => void;
  onDeleting: (deleting: boolean) => void;
}) {
  const { ref } = useProject();
  const queryClient = useQueryClient();
  const preview = $api.useMutation("post", "/api/v1/projects/{project}/deletions/preview");
  const remove = $api.useMutation("post", "/api/v1/projects/{project}/deletions");
  const body = requestBody(selection);

  // Radix unmounts closed content, so this body mounts once per opening and counts once.
  // biome-ignore lint/correctness/useExhaustiveDependencies: runs once per opening
  useEffect(() => {
    preview.mutate({ params: { path: { project: ref } }, body });
  }, []);

  const matched = preview.data?.matched_traces;
  const tooMany = problemOf(remove.error)?.code === "too_many_traces";
  const capped = preview.data?.capped === true || tooMany;
  const showCount = matched !== undefined && matched > 0 && !capped;
  const count = matched?.toLocaleString("en-US") ?? "";

  let description: string | undefined;
  if (preview.isPending || preview.isIdle) {
    description = "Counting matching traces…";
  } else if (matched === 0) {
    description = "No trace matches. Nothing to delete.";
  } else if (showCount) {
    description = `${count} ${matched === 1 ? "trace matches" : "traces match"} the current filter, search and time range. Their spans, scores and evaluation traces are hidden within seconds and erased within 24 hours. This cannot be undone.`;
  }

  const previewError = preview.isError
    ? problemOf(preview.error)?.code === "invalid_filter"
      ? "The filter is not valid. Fix it on the list first."
      : problemMessage(preview.error, "Could not count the traces.")
    : undefined;
  const removeError =
    remove.isError && !tooMany
      ? problemMessage(remove.error, "Could not request the deletion.")
      : undefined;
  const error = capped ? CAPPED : (previewError ?? removeError);

  function confirm() {
    onDeleting(true);
    remove.mutate(
      {
        params: { path: { project: ref } },
        body: { kind: "filter", ...body },
      },
      {
        onSuccess: () => {
          onDeleting(false);
          onClose();
          void invalidateProjectReads(queryClient);
        },
        onError: () => {
          onDeleting(false);
        },
      },
    );
  }

  const showConfirm = preview.isPending || preview.isIdle || showCount;

  return (
    <>
      <AlertDialogHeader>
        <AlertDialogTitle>Delete matching traces?</AlertDialogTitle>
        <AlertDialogDescription aria-live="polite">{description}</AlertDialogDescription>
      </AlertDialogHeader>
      <FormError message={error} />
      <AlertDialogFooter>
        <AlertDialogCancel disabled={remove.isPending}>
          {matched === 0 ? "Close" : "Cancel"}
        </AlertDialogCancel>
        {showConfirm ? (
          <Button
            type="button"
            variant="destructive"
            disabled={preview.isPending || preview.isIdle || remove.isPending}
            onClick={confirm}
          >
            {showCount ? `Delete ${count} ${matched === 1 ? "trace" : "traces"}` : "Delete traces"}
          </Button>
        ) : null}
      </AlertDialogFooter>
    </>
  );
}

/** The filter, search and range the list shows. Blank text is left out. */
function requestBody(selection: TraceSelection) {
  const filter = selection.filter?.trim();
  const search = selection.search?.trim();
  return {
    ...(filter ? { filter } : {}),
    ...(search ? { search } : {}),
    from: selection.from,
    to: selection.to,
    include_eval: selection.includeEval,
    include_experiments: selection.includeExperiments,
  };
}
