const PUBLIC_PATHS: ReadonlySet<string> = new Set([
  "/login",
  "/signup",
  "/setup",
  "/reset",
  "/verify",
]);

/** A relative path the server would also accept as `?redirect=` (`safe_redirect` in crates/server/src/auth/oidc/state_cookie.rs). */
export function safeRedirect(value: unknown): string | undefined {
  // The server's limit is 1024 bytes of UTF-8.
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    new TextEncoder().encode(value).length > 1024
  ) {
    return undefined;
  }
  if (!value.startsWith("/") || value.startsWith("//") || value.startsWith("/\\")) {
    return undefined;
  }
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code < 0x20 || code === 0x7f) {
      return undefined;
    }
  }
  return value;
}

/** Routes that render without a session (README § Routes). `/setup/workspace` is not one. */
export function isPublicPath(pathname: string): boolean {
  const path = pathname.length > 1 && pathname.endsWith("/") ? pathname.slice(0, -1) : pathname;
  return PUBLIC_PATHS.has(path) || path.startsWith("/invite/");
}

/** Full-page link that starts an OAuth/OIDC login (spec 08). */
export function authorizeHref(
  providerId: string,
  options: { redirect?: string; setupToken?: string } = {},
): string {
  const query = new URLSearchParams();
  if (options.redirect !== undefined) {
    query.set("redirect", options.redirect);
  }
  if (options.setupToken !== undefined) {
    query.set("setup_token", options.setupToken);
  }
  const base = `/api/auth/authorize/${encodeURIComponent(providerId)}`;
  const text = query.toString();
  return text.length > 0 ? `${base}?${text}` : base;
}
