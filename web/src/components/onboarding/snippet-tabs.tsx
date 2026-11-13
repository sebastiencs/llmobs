import { CopyButton } from "@/components/copy-button";
import {
  otelSnippet,
  PLACEHOLDER_KEY,
  pythonSnippet,
  typescriptSnippet,
  vercelSnippet,
} from "@/components/onboarding/snippets";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

const TABS = [
  { id: "otel", label: "OTel env vars", snippet: otelSnippet },
  { id: "python", label: "Python", snippet: pythonSnippet },
  { id: "typescript", label: "TypeScript", snippet: typescriptSnippet },
  { id: "vercel", label: "Vercel AI SDK", snippet: vercelSnippet },
] as const;

export function SnippetTabs({
  origin,
  apiKey = PLACEHOLDER_KEY,
}: {
  origin: string;
  apiKey?: string;
}) {
  return (
    <Tabs defaultValue="otel">
      <TabsList className="max-w-full">
        {TABS.map((tab) => (
          <TabsTrigger key={tab.id} value={tab.id}>
            {tab.label}
          </TabsTrigger>
        ))}
      </TabsList>
      {TABS.map((tab) => {
        const snippet = tab.snippet(origin, apiKey);
        return (
          <TabsContent key={tab.id} value={tab.id}>
            <div className="relative rounded-md border bg-muted/40">
              <div className="absolute top-2 right-2">
                <CopyButton value={snippet} label={tab.label} />
              </div>
              <pre className="overflow-auto p-3 pr-10 font-mono text-xs whitespace-pre">
                {snippet}
              </pre>
            </div>
          </TabsContent>
        );
      })}
    </Tabs>
  );
}
