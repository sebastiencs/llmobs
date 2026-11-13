import { expect, it } from "vitest";
import { adminUserError } from "./messages";

it("explains that an admin cannot change their own account", () => {
  expect(
    adminUserError({
      type: "about:blank",
      title: "Bad Request",
      status: 400,
      code: "bad_request",
      detail: "cannot change your own account",
    }),
  ).toBe("You can't change your own account.");
});

it("explains that a user is the last owner of an org", () => {
  expect(
    adminUserError({
      type: "about:blank",
      title: "Conflict",
      status: 409,
      code: "conflict",
      detail: "the user is the last owner of an org",
    }),
  ).toBe("This user is the last owner of an organization. Make someone else an owner first.");
});

it("explains that the instance needs an active admin", () => {
  expect(
    adminUserError({
      type: "about:blank",
      title: "Conflict",
      status: 409,
      code: "conflict",
      detail: "the instance would have no active administrator",
    }),
  ).toBe("This would leave the instance without an active admin.");
});

it("explains that the user is disabled", () => {
  expect(
    adminUserError({
      type: "about:blank",
      title: "Conflict",
      status: 409,
      code: "conflict",
      detail: "the user is disabled",
    }),
  ).toBe("This account is disabled. Enable it first.");
});

it("explains that the user no longer exists", () => {
  expect(
    adminUserError({ type: "about:blank", title: "Not Found", status: 404, code: "not_found" }),
  ).toBe("This user no longer exists.");
});

it("shows the problem's detail for other failures", () => {
  expect(
    adminUserError({
      type: "about:blank",
      title: "Internal Server Error",
      status: 500,
      code: "internal",
      detail: "boom",
    }),
  ).toBe("boom");
});

it("falls back to a generic message", () => {
  expect(adminUserError(new Error("x"))).toBe("The change failed. Try again.");
});
