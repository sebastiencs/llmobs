import type { Schema } from "@/api/types";
import { Badge } from "@/components/ui/badge";

/** The rule's state; a disabled rule shows "Disabled" whatever its state. */
export function StateBadge({ state, enabled }: { state: Schema<"AlertState">; enabled: boolean }) {
  if (!enabled) {
    return <Badge variant="ghost">Disabled</Badge>;
  }
  switch (state) {
    case "firing":
      return <Badge variant="destructive">Firing</Badge>;
    case "ok":
      return <Badge variant="secondary">OK</Badge>;
    case "no_data":
      return <Badge variant="outline">No data</Badge>;
  }
}
