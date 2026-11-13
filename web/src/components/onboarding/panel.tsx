import { Link } from "@tanstack/react-router";
import { SnippetTabs } from "@/components/onboarding/snippet-tabs";
import { API_KEY_ASK_NOTE, API_KEY_NOTE, WAITING_LINE } from "@/components/onboarding/snippets";
import { useOptionalProject } from "@/components/shell/project-context";
import { atLeast } from "@/lib/roles";

export function OnboardingPanel({ origin }: { origin: string }) {
  return (
    <div className="mx-auto flex w-full max-w-2xl flex-col gap-4 px-4 py-10">
      <div className="flex flex-col gap-1">
        <h2 className="text-lg font-medium">Send your first trace</h2>
        <p className="text-sm text-muted-foreground">{WAITING_LINE}</p>
      </div>
      <SnippetTabs origin={origin} />
      <ApiKeyNote />
    </div>
  );
}

function ApiKeyNote() {
  const project = useOptionalProject();
  if (!project) {
    return <p className="text-sm text-muted-foreground">{API_KEY_NOTE}</p>;
  }
  if (!atLeast(project.data.role, "admin")) {
    return <p className="text-sm text-muted-foreground">{API_KEY_ASK_NOTE}</p>;
  }
  return (
    <p className="text-sm text-muted-foreground">
      <Link
        to="/$org/$project/settings/api-keys"
        params={{ org: project.org, project: project.project }}
        className="underline underline-offset-4"
      >
        {API_KEY_NOTE}
      </Link>
    </p>
  );
}
