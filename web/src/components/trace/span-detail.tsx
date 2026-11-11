import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { problemOf } from "@/api/problem";
import { spanDetailQuery } from "@/api/queries";
import { useProject } from "@/components/shell/project-context";
import { AttributesTab } from "@/components/trace/attributes-tab";
import { IoTab } from "@/components/trace/io-tab";
import { SpanHeader } from "@/components/trace/span-header";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { TooltipProvider } from "@/components/ui/tooltip";
import { STORAGE_PREFIX } from "@/product";

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
            <IoTab span={detail} />
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
