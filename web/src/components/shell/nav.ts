import { Waypoints } from "lucide-react";

/** Project pages that exist. Later sub-phases append their entry here. */
export const projectNav = [
  { to: "/$org/$project/traces", label: "Traces", icon: Waypoints },
] as const;
