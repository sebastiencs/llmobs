import { api } from "@/api/client";
import { type Problem, problemOf } from "@/api/problem";
import type { Schema } from "@/api/types";
import { sqlProblemMessage } from "@/lib/sql/format";

export type ExportFormat = "csv" | "ndjson" | "parquet";

export const EXPORT_FORMATS: Record<ExportFormat, { label: string; accept: string }> = {
  csv: { label: "CSV", accept: "text/csv" },
  ndjson: { label: "NDJSON", accept: "application/x-ndjson" },
  parquet: { label: "Parquet", accept: "application/vnd.apache.parquet" },
};

export const EXPORT_ORDER: readonly ExportFormat[] = ["csv", "ndjson", "parquet"];

export const EXPORT_ROLE_REASON = "Exports need the member role";
export const EXPORT_CUT = "The export failed before it finished";
export const EXPORT_NOT_SAVED = "The file could not be saved";

/** `query-20260101T093005Z.csv` for `now`, in UTC. The extension is the format id. */
export function defaultExportName(format: ExportFormat, now: Date): string {
  const stamp = now
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d{3}Z$/, "Z");
  return `query-${stamp}.${format}`;
}

/** The `filename="…"` of a `Content-Disposition`, else `fallback`. Only `[A-Za-z0-9._-]+`. */
export function exportName(disposition: string | null, fallback: string): string {
  const match = disposition ? /filename="([A-Za-z0-9._-]+)"/.exec(disposition) : null;
  const name = match?.[1];
  return name && !name.startsWith(".") ? name : fallback;
}

/** Saves `blob` as `name` through a temporary object URL, revoked on the next task. */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.rel = "noopener";
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function exportProblemMessage(problem: Problem | undefined): string {
  if (!problem) {
    return "The export could not be downloaded";
  }
  return problem.code === "forbidden" ? EXPORT_ROLE_REASON : sqlProblemMessage(problem);
}

export type ExportOutcome = { ok: true; name: string } | { ok: false; message: string };

/** Runs the export and saves the file. Never throws. */
export async function downloadSqlExport(input: {
  projectRef: string;
  body: Schema<"SqlQueryRequest">;
  format: ExportFormat;
}): Promise<ExportOutcome> {
  let blob: Blob;
  let disposition: string | null;
  try {
    const { data, error, response } = await api.POST("/api/v1/projects/{project}/sql", {
      params: { path: { project: input.projectRef } },
      body: input.body,
      headers: { Accept: EXPORT_FORMATS[input.format].accept },
      parseAs: "blob",
    });
    if (error !== undefined || !response.ok) {
      return { ok: false, message: exportProblemMessage(problemOf(error)) };
    }
    blob = data ?? new Blob([]);
    disposition = response.headers.get("Content-Disposition");
  } catch {
    return { ok: false, message: EXPORT_CUT };
  }
  const name = exportName(disposition, defaultExportName(input.format, new Date()));
  try {
    saveBlob(blob, name);
  } catch {
    return { ok: false, message: EXPORT_NOT_SAVED };
  }
  return { ok: true, name };
}
