import { Database, LayoutDashboard, MessagesSquare, Rows3, Users, Waypoints } from "lucide-react";

/** Project pages that exist. Later sub-phases append their entry here. */
export const projectNav = [
  { to: "/$org/$project/dashboard", label: "Dashboard", icon: LayoutDashboard },
  { to: "/$org/$project/traces", label: "Traces", icon: Waypoints },
  { to: "/$org/$project/spans", label: "Spans", icon: Rows3 },
  { to: "/$org/$project/sessions", label: "Sessions", icon: MessagesSquare },
  { to: "/$org/$project/users", label: "Users", icon: Users },
  { to: "/$org/$project/sql", label: "SQL", icon: Database },
] as const;
