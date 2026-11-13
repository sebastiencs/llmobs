import type { Middleware } from "openapi-fetch";

type UnauthorizedHandler = (apiPath: string) => void;

let handler: UnauthorizedHandler | undefined;

/** The router registers the one handler (D-L-1). A later call replaces it. */
export function setUnauthorizedHandler(next: UnauthorizedHandler | undefined): void {
  handler = next;
}

let signingOut = false;

export function setSigningOut(value: boolean): void {
  signingOut = value;
}

/**
 * True from the moment a sign-out starts until its navigation is done. The handler
 * ignores 401s meanwhile, so a poll that fails mid-sign-out cannot race the sign-out's
 * own navigation to `/login` with a `/login?redirect=…`.
 */
export function isSigningOut(): boolean {
  return signingOut;
}

/**
 * API paths whose 401 means "your session ended": every `/api/v1/*` path, and exactly
 * `/api/auth/me`. Other `/api/auth/*` paths answer 401 for a wrong password.
 */
export function isSessionApiPath(pathname: string): boolean {
  return pathname.startsWith("/api/v1/") || pathname === "/api/auth/me";
}

export const unauthorizedMiddleware: Middleware = {
  onResponse({ request, response }) {
    const pathname = new URL(request.url).pathname;
    if (response.status === 401 && isSessionApiPath(pathname)) {
      handler?.(pathname);
    }
    return undefined;
  },
};
