import type { Schema } from "@/api/types";
import { Button } from "@/components/ui/button";
import { authorizeHref } from "@/lib/redirect";

export function ProviderButtons({
  providers,
  redirect,
  setupToken,
}: {
  providers: Schema<"AuthProvider">[];
  redirect?: string;
  setupToken?: string;
}) {
  if (providers.length === 0) {
    return null;
  }
  return (
    <div className="flex flex-col gap-2">
      {providers.map((provider) => (
        <Button key={provider.id} asChild variant="outline" className="w-full">
          <a href={authorizeHref(provider.id, { redirect, setupToken })}>
            Continue with {provider.display_name}
          </a>
        </Button>
      ))}
    </div>
  );
}

export function OrDivider() {
  return (
    <div className="flex items-center gap-3 text-xs text-muted-foreground">
      <span className="h-px flex-1 bg-border" />
      or
      <span className="h-px flex-1 bg-border" />
    </div>
  );
}
