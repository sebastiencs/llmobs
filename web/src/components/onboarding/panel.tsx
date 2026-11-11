import { CopyButton } from "@/components/copy-button";
import {
  API_KEY_NOTE,
  otelSnippet,
  pythonSnippet,
  typescriptSnippet,
  vercelSnippet,
  WAITING_LINE,
} from "@/components/onboarding/snippets";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

const TABS = [
  { id: "otel", label: "OTel env vars", snippet: otelSnippet },
  { id: "python", label: "Python", snippet: pythonSnippet },
  { id: "typescript", label: "TypeScript", snippet: typescriptSnippet },
  { id: "vercel", label: "Vercel AI SDK", snippet: vercelSnippet },
] as const;

export function OnboardingPanel({ origin }: { origin: string }) {
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-4 px-4 py-10">
      <div className="flex flex-col gap-1">
        <h2 className="text-lg font-medium">Send your first trace</h2>
        <p className="text-sm text-muted-foreground">{WAITING_LINE}</p>
      </div>
      <Tabs defaultValue="otel">
        <TabsList className="max-w-full">
          {TABS.map((tab) => (
            <TabsTrigger key={tab.id} value={tab.id}>
              {tab.label}
            </TabsTrigger>
          ))}
        </TabsList>
        {TABS.map((tab) => {
          const snippet = tab.snippet(origin);
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
      <p className="text-sm text-muted-foreground">{API_KEY_NOTE}</p>
    </div>
  );
}
