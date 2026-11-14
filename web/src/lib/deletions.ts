import type { Schema } from "@/api/types";

type Deletion = Schema<"Deletion">;

const KIND_LABELS: Record<Deletion["kind"], string> = {
  user: "User",
  trace_ids: "Traces by id",
  filter: "Traces by filter",
  project: "Project",
  org: "Organization",
};

const STATUS_LABELS: Record<Deletion["status"], string> = {
  resolving: "Finding data",
  pending: "Hidden, erasing",
  applied: "Erased",
  failed: "Failed",
};

const ERROR_LABELS: Record<NonNullable<Deletion["error"]>, string> = {
  too_many_traces: "Too many traces matched (over 100,000)",
  resolution_failed: "Could not find the data to delete",
};

/** "User", "Traces by id", "Traces by filter", "Project", "Organization". */
export function deletionKindLabel(kind: Deletion["kind"]): string {
  return KIND_LABELS[kind];
}

/** "Finding data", "Hidden, erasing", "Erased", "Failed". */
export function deletionStatusLabel(status: Deletion["status"]): string {
  return STATUS_LABELS[status];
}

/** `count(1, "trace")` → "1 trace", `count(3, "trace")` → "3 traces". */
export function count(value: number, noun: string): string {
  return `${value} ${value === 1 ? noun : `${noun}s`}`;
}

/** What was deleted, in words. A `filter` appends its trace count when one is known. */
export function deletionSubject(deletion: Deletion): string {
  switch (deletion.kind) {
    case "user":
      return `user ${deletion.user_id ?? ""}`;
    case "trace_ids":
      return count(deletion.requested_traces ?? 0, "trace");
    case "filter": {
      const selection =
        deletion.filter?.filter ??
        (deletion.filter?.search !== undefined
          ? `search "${deletion.filter.search}"`
          : "all traces");
      return deletion.trace_count === undefined
        ? selection
        : `${selection} (${count(deletion.trace_count, "trace")})`;
    }
    case "project":
    case "org":
      return `${deletion.name ?? deletion.slug ?? "unknown"} (${deletion.slug ?? "?"})`;
  }
}

/** How often the history refetches while a deletion is open. Tests lower it. */
export const DELETION_POLL_MS = 5_000;

let pollMs = DELETION_POLL_MS;

export function deletionPollMs(): number {
  return pollMs;
}

/** Tests pass a short interval. The history reads it on each render. */
export function setDeletionPollMs(ms: number): void {
  pollMs = ms;
}

/** "Too many traces matched (over 100,000)" or "Could not find the data to delete". */
export function deletionErrorLabel(error: NonNullable<Deletion["error"]>): string {
  return ERROR_LABELS[error];
}

/** True while the status is `resolving` or `pending`. */
export function isOpenDeletion(deletion: Deletion): boolean {
  return deletion.status === "resolving" || deletion.status === "pending";
}
