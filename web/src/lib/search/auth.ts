import { safeRedirect } from "@/lib/redirect";
import { parseEnum, parseString } from "@/lib/search/common";

/** `?error=` codes of `GET /api/auth/callback/{provider}` (spec 08 § OAuth / OIDC). */
export const OAUTH_ERROR_CODES = [
  "invalid_state",
  "provider_error",
  "provider_unavailable",
  "email_not_verified",
  "domain_not_allowed",
  "signup_closed",
  "account_disabled",
  "link_refused",
  "setup_required",
] as const;

export type OAuthErrorCode = (typeof OAUTH_ERROR_CODES)[number];

/** Longest email address the UI keeps in the URL. */
export const EMAIL_MAX = 254;

export type LoginSearch = {
  redirect?: string;
  error?: OAuthErrorCode;
  email?: string;
};

/** Every key is returned, `undefined` when rejected (see L3 step 2). */
export function parseLoginSearch(search: Record<string, unknown>): LoginSearch {
  return {
    redirect: safeRedirect(search.redirect),
    error: parseEnum(search.error, OAUTH_ERROR_CODES),
    email: parseString(search.email, EMAIL_MAX),
  };
}

/** Longest token the UI keeps from a `?token=` link. Server tokens are 43 characters. */
export const TOKEN_MAX = 256;

export type SetupSearch = { token?: string };

/** Returns `token` even when rejected (`undefined`), as L3 step 2 explains. */
export function parseSetupSearch(search: Record<string, unknown>): SetupSearch {
  return { token: parseString(search.token, TOKEN_MAX) };
}
