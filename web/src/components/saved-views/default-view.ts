import { useQuery } from "@tanstack/react-query";
import { useLocation } from "@tanstack/react-router";
import { useEffect, useRef } from "react";
import { meQuery, savedViewsQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useProject } from "@/components/shell/project-context";
import { searchFromViewState } from "@/lib/search/common";

declare module "@tanstack/react-router" {
  interface HistoryState {
    /** Set by navigation that enters a list page (sidebar, palette, g shortcuts, project switcher). */
    applyDefaultView?: true;
  }
}

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

/** Applies the caller's default view when the list page is entered with no search params (D-C-2). */
export function useDefaultSavedView(
  kind: Schema<"SavedViewKind">,
  search: object,
  onApply: (search: Record<string, unknown>) => void,
): { hold: boolean } {
  const { org, project } = useProject();
  const location = useLocation();
  const entryKey = location.state.__TSR_key;
  const mountKey = useRef(entryKey);
  const appliedKey = useRef<string | undefined>(undefined);
  const views = useQuery(savedViewsQuery(org, project, kind));
  const me = useQuery(meQuery);
  const empty = Object.keys(search).length === 0;
  const entered = entryKey === mountKey.current || location.state.applyDefaultView === true;
  const pending = empty && entered && appliedKey.current !== entryKey;
  const settled = views.isFetched && me.isFetched;
  const match =
    pending && settled
      ? defaultSavedView(views.data?.data ?? [], kind, me.data?.user.id)
      : undefined;

  useEffect(() => {
    if (!match) {
      return;
    }
    appliedKey.current = entryKey;
    onApply({ ...searchFromViewState(match.state), view: match.id });
  }, [match, onApply, entryKey]);

  return { hold: pending && (!settled || match !== undefined) };
}
