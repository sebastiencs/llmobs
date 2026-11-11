import { MessagesSquare, Rows3, Waypoints } from "lucide-react";

/** Project pages that exist. Later sub-phases append their entry here. */
export const projectNav = [
  { to: "/$org/$project/traces", label: "Traces", icon: Waypoints },
  { to: "/$org/$project/spans", label: "Spans", icon: Rows3 },
  { to: "/$org/$project/sessions", label: "Sessions", icon: MessagesSquare },
] as const;
