import { problemMessage, problemOf } from "@/api/problem";
import type { OAuthErrorCode } from "@/lib/search/auth";

export const NO_SIGN_IN_METHOD =
  "No sign-in method is configured on this server. An instance admin can create an account with `llmobs admin create-user`.";

export const RATE_LIMITED = "Too many attempts. Wait a minute and try again.";

const OAUTH_MESSAGES: Record<OAuthErrorCode, string> = {
  invalid_state: "The sign-in took too long or was started in another tab. Try again.",
  provider_error: "The identity provider returned an error. Try again.",
  provider_unavailable: "The identity provider could not be reached. Try again later.",
  email_not_verified: "Your identity provider did not confirm your email address.",
  domain_not_allowed: "Your email domain is not allowed to sign in here.",
  signup_closed:
    "No account exists for this identity, and sign-up is closed. Ask an admin for an invitation.",
  account_disabled: "This account is disabled. Contact an instance admin.",
  link_refused:
    "An account with this email already exists and could not be linked to this provider. Sign in with your password.",
  setup_required: "This server is not set up yet. Open the setup link printed in the server log.",
};

export function oauthErrorMessage(code: OAuthErrorCode): string {
  return OAUTH_MESSAGES[code];
}

/** The message above the sign-in button for a failed `POST /api/auth/login`. */
export function loginErrorMessage(error: unknown): string {
  const problem = problemOf(error);
  if (problem?.status === 401) {
    return "Incorrect email or password.";
  }
  if (problem?.status === 403 && problem.code === "sso_required") {
    return "Your email domain must sign in with single sign-on. Use a button above.";
  }
  if (problem?.status === 403 && problem.code === "email_not_verified") {
    return "Verify your email address before signing in.";
  }
  // The CSRF layer also answers 403 `forbidden` (D141); its detail says so.
  if (problem?.status === 403 && problem.detail === "origin check failed") {
    return problem.detail;
  }
  if (problem?.status === 403) {
    return "Password sign-in is disabled on this server.";
  }
  if (problem?.status === 429) {
    return RATE_LIMITED;
  }
  return problemMessage(error, "Sign-in failed. Try again.");
}
