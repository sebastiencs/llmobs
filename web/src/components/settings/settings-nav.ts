export const orgSettingsNav = [
  { to: "/$org/settings/general", label: "General" },
  { to: "/$org/settings/members", label: "Members" },
  { to: "/$org/settings/llm-connections", label: "LLM connections" },
] as const;

export const projectSettingsNav = [
  { to: "/$org/$project/settings/general", label: "General", adminOnly: false },
  { to: "/$org/$project/settings/api-keys", label: "API keys", adminOnly: true },
  { to: "/$org/$project/settings/members", label: "Members", adminOnly: false },
  { to: "/$org/$project/settings/channels", label: "Channels", adminOnly: false },
] as const;

/** True on `/<org>/<project>/settings` and every path below it. Slugs are `[a-z0-9-]`. */
export function isProjectSettingsPath(pathname: string, org: string, project: string): boolean {
  const base = `/${org}/${project}/settings`;
  return pathname === base || pathname.startsWith(`${base}/`);
}
