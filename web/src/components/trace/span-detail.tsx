import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { problemOf } from "@/api/problem";
import { spanDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { KindIcon } from "@/components/kind-icon";
import { useProject } from "@/components/shell/project-context";
import { StatusDot } from "@/components/status-dot";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { formatDuration } from "@/lib/format";

/** Pretty JSON past this length is not drawn until the user asks for the raw text. */
const RENDER_CAP = 1_000_000;

export function SpanDetail({ traceId, spanId }: { traceId: string; spanId: string }) {
  const { org, project } = useProject();
  const span = useQuery(spanDetailQuery(org, project, traceId, spanId));

  if (span.isPending) {
    return (
      <div className="flex flex-col gap-2 p-3">
        <Skeleton className="h-4 w-2/3" />
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-4 w-1/2" />
      </div>
    );
  }
  if (span.isError) {
    return (
      <p className="p-3 text-sm">{problemOf(span.error)?.title ?? "Could not load this span"}</p>
    );
  }

  const detail = span.data;
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-[32px] shrink-0 items-center gap-2 border-b px-3">
        <KindIcon kind={detail.span_kind} />
        <span className="min-w-0 truncate text-sm">{detail.name}</span>
        {detail.model ? (
          <span className="min-w-0 truncate text-xs text-muted-foreground">{detail.model}</span>
        ) : null}
        <span className="num shrink-0 text-xs">{formatDuration(detail.duration_ns)}</span>
        <StatusDot status={detail.status} />
        {detail.status_message ? (
          <span className="min-w-0 truncate text-xs">{detail.status_message}</span>
        ) : null}
      </div>
      <Tabs defaultValue="io" className="min-h-0 flex-1 gap-0">
        <TabsList variant="line" className="h-7 px-3">
          <TabsTrigger value="io" className="text-xs">
            Input / Output
          </TabsTrigger>
        </TabsList>
        <TabsContent value="io" className="min-h-0 overflow-auto px-3 pb-3">
          <div className="flex flex-col gap-3">
            <PayloadBlock label="Input" payload={detail.input} />
            <PayloadBlock label="Output" payload={detail.output} />
          </div>
        </TabsContent>
      </Tabs>
    </div>
  );
}

export function PayloadBlock({
  label,
  payload,
}: {
  label: string;
  payload: Schema<"Payload"> | undefined;
}) {
  const text = payloadText(payload);
  const [showRaw, setShowRaw] = useState(false);
  return (
    <section className="flex min-w-0 flex-col gap-1">
      <h3 className="text-xs font-medium">{label}</h3>
      {text === undefined ? (
        <p className="text-xs text-muted-foreground">—</p>
      ) : text.pretty.length > RENDER_CAP && !showRaw ? (
        <button
          type="button"
          className="self-start text-xs underline"
          onClick={() => setShowRaw(true)}
        >
          Show raw
        </button>
      ) : (
        <pre className="overflow-auto font-mono text-xs whitespace-pre-wrap">
          {text.pretty.length > RENDER_CAP ? text.raw : text.pretty}
        </pre>
      )}
    </section>
  );
}

function payloadText(
  payload: Schema<"Payload"> | undefined,
): { pretty: string; raw: string } | undefined {
  if (!payload) {
    return undefined;
  }
  if (payload.format === "text") {
    return { pretty: payload.value, raw: payload.value };
  }
  return {
    pretty: JSON.stringify(payload.value, null, 2),
    raw: JSON.stringify(payload.value),
  };
}
