import { cn } from "cn";
import type { Schema } from "@/api/types";

const STYLES = {
  ok: "bg-status-ok",
  unset: "bg-status-unset",
  error: "bg-status-error ring-2 ring-status-error ring-offset-1 ring-offset-background",
} as const satisfies Record<Schema<"SpanStatus">, string>;

export function StatusDot({ status }: { status: Schema<"SpanStatus"> }) {
  return (
    <span className="inline-flex">
      <span aria-hidden="true" className={cn("size-2 shrink-0 rounded-full", STYLES[status])} />
      <span className="sr-only">{status}</span>
    </span>
  );
}
