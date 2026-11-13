import { expect, it } from "vitest";
import { OAUTH_ERROR_CODES } from "@/lib/search/auth";
import { loginErrorMessage, oauthErrorMessage, RATE_LIMITED } from "./messages";

it("has a message for every OAuth error code", () => {
  for (const code of OAUTH_ERROR_CODES) {
    const message = oauthErrorMessage(code);
    expect(message.length).toBeGreaterThan(0);
    expect(message.endsWith(".")).toBe(true);
  }
});

it("maps login problems to messages", () => {
  const problem = (status: number, code: string, detail?: string) => ({
    type: "about:blank",
    title: "Problem",
    status,
    code,
    ...(detail === undefined ? {} : { detail }),
  });

  expect(loginErrorMessage(problem(401, "unauthorized"))).toBe("Incorrect email or password.");
  expect(loginErrorMessage(problem(403, "sso_required"))).toBe(
    "Your email domain must sign in with single sign-on. Use a button above.",
  );
  expect(loginErrorMessage(problem(403, "email_not_verified"))).toBe(
    "Verify your email address before signing in.",
  );
  expect(loginErrorMessage(problem(403, "forbidden"))).toBe(
    "Password sign-in is disabled on this server.",
  );
  expect(loginErrorMessage(problem(403, "forbidden", "origin check failed"))).toBe(
    "origin check failed",
  );
  expect(loginErrorMessage(problem(429, "rate_limited"))).toBe(RATE_LIMITED);
  expect(loginErrorMessage(problem(500, "internal", "boom"))).toBe("boom");
  expect(loginErrorMessage(new Error("offline"))).toBe("Sign-in failed. Try again.");
});
