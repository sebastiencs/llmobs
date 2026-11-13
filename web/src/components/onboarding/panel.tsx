import { SnippetTabs } from "@/components/onboarding/snippet-tabs";
import { API_KEY_NOTE, WAITING_LINE } from "@/components/onboarding/snippets";

export function OnboardingPanel({ origin }: { origin: string }) {
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-4 px-4 py-10">
      <div className="flex flex-col gap-1">
        <h2 className="text-lg font-medium">Send your first trace</h2>
        <p className="text-sm text-muted-foreground">{WAITING_LINE}</p>
      </div>
      <SnippetTabs origin={origin} />
      <p className="text-sm text-muted-foreground">{API_KEY_NOTE}</p>
    </div>
  );
}
