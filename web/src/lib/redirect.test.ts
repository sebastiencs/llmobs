import { expect, it } from "vitest";
import { authorizeHref, isPublicPath, safeRedirect } from "./redirect";

it("keeps relative paths", () => {
  expect(safeRedirect("/acme/support-bot/traces?range=1h")).toBe(
    "/acme/support-bot/traces?range=1h",
  );
  expect(safeRedirect("/")).toBe("/");
});

it("drops anything that could leave the origin", () => {
  expect(safeRedirect("https://evil.example")).toBeUndefined();
  expect(safeRedirect("//evil.example")).toBeUndefined();
  expect(safeRedirect("/\\evil.example")).toBeUndefined();
  expect(safeRedirect("acme")).toBeUndefined();
  expect(safeRedirect("")).toBeUndefined();
  expect(safeRedirect("/a\nb")).toBeUndefined();
  expect(safeRedirect("/a\u007fb")).toBeUndefined();
  expect(safeRedirect(`/${"a".repeat(1024)}`)).toBeUndefined();
  expect(safeRedirect(`/${"é".repeat(600)}`)).toBeUndefined();
  expect(safeRedirect(42)).toBeUndefined();
  expect(safeRedirect(undefined)).toBeUndefined();
  expect(safeRedirect(`/${"a".repeat(1023)}`)).toBe(`/${"a".repeat(1023)}`);
});

it("lists the public routes", () => {
  expect(isPublicPath("/login")).toBe(true);
  expect(isPublicPath("/login/")).toBe(true);
  expect(isPublicPath("/signup")).toBe(true);
  expect(isPublicPath("/setup")).toBe(true);
  expect(isPublicPath("/setup/")).toBe(true);
  expect(isPublicPath("/reset")).toBe(true);
  expect(isPublicPath("/verify")).toBe(true);
  expect(isPublicPath("/invite/abc")).toBe(true);
  expect(isPublicPath("/setup/workspace")).toBe(false);
  expect(isPublicPath("/cli-auth")).toBe(false);
  expect(isPublicPath("/account")).toBe(false);
  expect(isPublicPath("/acme")).toBe(false);
  expect(isPublicPath("/loginx")).toBe(false);
});

it("builds authorize links", () => {
  expect(authorizeHref("github")).toBe("/api/auth/authorize/github");
  expect(authorizeHref("oidc:okta", { redirect: "/acme" })).toBe(
    "/api/auth/authorize/oidc%3Aokta?redirect=%2Facme",
  );
  expect(authorizeHref("github", { redirect: "/setup/workspace", setupToken: "t1" })).toBe(
    "/api/auth/authorize/github?redirect=%2Fsetup%2Fworkspace&setup_token=t1",
  );
});
