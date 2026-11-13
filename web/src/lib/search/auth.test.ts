import { expect, it } from "vitest";
import { parseLoginSearch } from "./auth";

it("keeps a safe redirect, a known error and an email", () => {
  expect(
    parseLoginSearch({ redirect: "/acme", error: "signup_closed", email: "ada@example.com" }),
  ).toEqual({ redirect: "/acme", error: "signup_closed", email: "ada@example.com" });
});

it("drops an unsafe redirect, an unknown error and an empty email", () => {
  const result = parseLoginSearch({ redirect: "//evil.example", error: "nope", email: "  " });
  expect(Object.keys(result).sort()).toEqual(["email", "error", "redirect"]);
  expect(result.redirect).toBeUndefined();
  expect(result.error).toBeUndefined();
  expect(result.email).toBeUndefined();
});

it("drops values that are not strings", () => {
  const result = parseLoginSearch({ redirect: 5, error: 1, email: true });
  expect(Object.keys(result).sort()).toEqual(["email", "error", "redirect"]);
  expect(result.redirect).toBeUndefined();
  expect(result.error).toBeUndefined();
  expect(result.email).toBeUndefined();
});
