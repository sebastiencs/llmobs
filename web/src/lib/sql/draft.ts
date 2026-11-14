import { useCallback, useState } from "react";
import { STORAGE_PREFIX } from "@/product";

export type SqlDraft = {
  query: string;
};

/** Changes a draft. Always pass a function so two updates in one event both apply. */
export type DraftUpdate = (update: (previous: SqlDraft) => SqlDraft) => void;

export const DEFAULT_QUERY =
  "SELECT name, count(*) AS trace_count\nFROM traces\nGROUP BY name\nORDER BY trace_count DESC\nLIMIT 100";

export function draftKey(projectRef: string): string {
  return `${STORAGE_PREFIX}sql.draft.${projectRef}`;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function readDraft(projectRef: string): SqlDraft {
  try {
    const raw = localStorage.getItem(draftKey(projectRef));
    if (raw === null) {
      return { query: DEFAULT_QUERY };
    }
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || typeof parsed.query !== "string") {
      return { query: DEFAULT_QUERY };
    }
    return { query: parsed.query };
  } catch {
    return { query: DEFAULT_QUERY };
  }
}

export function writeDraft(projectRef: string, draft: SqlDraft): void {
  try {
    localStorage.setItem(draftKey(projectRef), JSON.stringify(draft));
  } catch {
    // Private mode or a full quota: the draft still lives in memory for this page.
  }
}

/** The draft of `projectRef`. Each update is written through to localStorage. */
export function useSqlDraft(projectRef: string): [SqlDraft, DraftUpdate] {
  const [draft, setDraft] = useState(() => readDraft(projectRef));
  const update = useCallback<DraftUpdate>(
    (change) => {
      setDraft((previous) => {
        const next = change(previous);
        writeDraft(projectRef, next);
        return next;
      });
    },
    [projectRef],
  );
  return [draft, update];
}
