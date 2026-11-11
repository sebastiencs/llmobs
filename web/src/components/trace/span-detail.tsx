import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { problemOf } from "@/api/problem";
import { spanDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useProject } from "@/components/shell/project-context";
import { AttributesTab } from "@/components/trace/attributes-tab";
import { ChatTab } from "@/components/trace/chat-tab";
import { IoTab } from "@/components/trace/io-tab";
import { SpanHeader } from "@/components/trace/span-header";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { TooltipProvider } from "@/components/ui/tooltip";
import { STORAGE_PREFIX } from "@/product";

const TAB_KEY = `${STORAGE_PREFIX}span-tab`;
type SpanTab = "chat" | "io" | "attributes";

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
  const [choice, setChoice] = useState(readStoredTab);
  function select(value: string) {
    if (value !== "chat" && value !== "io" && value !== "attributes") return;
    setChoice(value);
    localStorage.setItem(TAB_KEY, value);
  }

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
  const messages = hasMessages(detail);
  const tab = activeTab(choice, messages);
  return (
    <TooltipProvider>
      <section aria-label="Span" className="flex min-h-0 flex-1 flex-col">
        <SpanHeader span={detail} detached={detached} />
        <Tabs value={tab} onValueChange={select} className="min-h-0 flex-1 gap-0">
          <TabsList variant="line" className="h-7 px-3">
            {messages ? (
              <TabsTrigger value="chat" className="text-xs">
                Chat
              </TabsTrigger>
            ) : null}
            <TabsTrigger value="io" className="text-xs">
              Input / Output
            </TabsTrigger>
            <TabsTrigger value="attributes" className="text-xs">
              Attributes
            </TabsTrigger>
          </TabsList>
          {messages ? (
            <TabsContent value="chat" className="min-h-0 overflow-auto px-3 pb-3">
              <ChatTab span={detail} />
            </TabsContent>
          ) : null}
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

function hasMessages(span: Schema<"SpanDetail">): boolean {
  return span.input?.format === "messages" || span.output?.format === "messages";
}

function readStoredTab(): SpanTab | null {
  const stored = localStorage.getItem(TAB_KEY);
  if (stored === "chat" || stored === "io" || stored === "attributes") return stored;
  return null;
}

function activeTab(choice: SpanTab | null, messages: boolean): SpanTab {
  if (choice === "attributes") return "attributes";
  if (choice === "io") return "io";
  if (choice === "chat" && messages) return "chat";
  if (messages) return "chat";
  return "io";
}
