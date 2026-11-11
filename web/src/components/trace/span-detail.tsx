import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { problemOf } from "@/api/problem";
import { spanDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useProject } from "@/components/shell/project-context";
import { AttributesTab } from "@/components/trace/attributes-tab";
import { SpanHeader } from "@/components/trace/span-header";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { TooltipProvider } from "@/components/ui/tooltip";
import { STORAGE_PREFIX } from "@/product";

/** Pretty JSON past this length is not drawn until the user asks for the raw text. */
const RENDER_CAP = 1_000_000;
const TAB_KEY = `${STORAGE_PREFIX}span-tab`;
const TABS = new Set(["io", "attributes"]);

export function SpanDetail({
  traceId,
  spanId,
  detached = false,
  onAddFilter,
}: {
  traceId: string;
  spanId: string;
  detached?: boolean;
  onAddFilter?: (conjunct: string) => void;
}) {
  const { org, project } = useProject();
  const span = useQuery(spanDetailQuery(org, project, traceId, spanId));
  const [tab, setTab] = useState(storedTab);

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
    <TooltipProvider>
      <section aria-label="Span" className="flex min-h-0 flex-1 flex-col">
        <SpanHeader span={detail} detached={detached} />
        <Tabs
          value={tab}
          onValueChange={(value) => {
            setTab(value);
            localStorage.setItem(TAB_KEY, value);
          }}
          className="min-h-0 flex-1 gap-0"
        >
          <TabsList variant="line" className="h-7 px-3">
            <TabsTrigger value="io" className="text-xs">
              Input / Output
            </TabsTrigger>
            <TabsTrigger value="attributes" className="text-xs">
              Attributes
            </TabsTrigger>
          </TabsList>
          <TabsContent value="io" className="min-h-0 overflow-auto px-3 pb-3">
            <div className="flex flex-col gap-3">
              <PayloadBlock label="Input" payload={detail.input} />
              <PayloadBlock label="Output" payload={detail.output} />
            </div>
          </TabsContent>
          <TabsContent value="attributes" className="min-h-0 overflow-auto px-3 pb-3">
            <AttributesTab span={detail} onAddFilter={onAddFilter} />
          </TabsContent>
        </Tabs>
      </section>
    </TooltipProvider>
  );
}

function storedTab(): string {
  const stored = localStorage.getItem(TAB_KEY);
  return stored && TABS.has(stored) ? stored : "io";
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
