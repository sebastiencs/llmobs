import { useQuery } from "@tanstack/react-query";
import { useEffect } from "react";
import { meQuery, savedViewsQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useProject } from "@/components/shell/project-context";
import { searchFromViewState } from "@/lib/search/common";

type SavedView = Schema<"SavedView">;

export function defaultSavedView(
  views: readonly SavedView[],
  kind: string,
  userId: string | undefined,
): SavedView | undefined {
  const defaults = views.filter((view) => view.kind === kind && view.is_default);
  const own = defaults.find((view) => userId !== undefined && view.owner_user_id === userId);
  if (own) {
    return own;
  }
  return defaults.find((view) => view.shared);
}

/** Applies the caller's default view when the page was opened with no search params. */
export function useDefaultSavedView(
  kind: Schema<"SavedViewKind">,
  search: object,
  onApply: (search: Record<string, unknown>) => void,
): { hold: boolean } {
  const { org, project } = useProject();
  const views = useQuery(savedViewsQuery(org, project, kind));
  const me = useQuery(meQuery);
  const empty = Object.keys(search).length === 0;
  const settled = views.isFetched && me.isFetched;
  const match =
    empty && settled ? defaultSavedView(views.data?.data ?? [], kind, me.data?.user.id) : undefined;

  useEffect(() => {
    if (!match) {
      return;
    }
    onApply({ ...searchFromViewState(match.state), view: match.id });
  }, [match, onApply]);

  return { hold: empty && (!settled || match !== undefined) };
}
