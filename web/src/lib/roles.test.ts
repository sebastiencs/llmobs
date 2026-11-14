import { expect, it } from "vitest";
import {
  atLeast,
  canChangeRole,
  canHaveOverride,
  canManageConnections,
  canManageEvaluation,
  canRemoveMember,
  grantableRoles,
  overrideLabel,
  roleLabel,
  roleRank,
} from "./roles";

it("ranks roles", () => {
  expect(roleRank("viewer")).toBe(0);
  expect(roleRank("owner")).toBe(3);
  expect(atLeast("admin", "member")).toBe(true);
  expect(atLeast("member", "admin")).toBe(false);
  expect(atLeast(undefined, "viewer")).toBe(false);
});

it("labels roles", () => {
  expect(roleLabel("admin")).toBe("Admin");
  expect(overrideLabel("inherit")).toBe("Inherit org role");
  expect(overrideLabel("none")).toBe("No access");
});

it("grants no role above the actor's", () => {
  expect(grantableRoles("owner")).toEqual(["owner", "admin", "member", "viewer"]);
  expect(grantableRoles("admin")).toEqual(["admin", "member", "viewer"]);
  expect(grantableRoles("member")).toEqual([]);
  expect(grantableRoles("viewer")).toEqual([]);
  expect(grantableRoles(undefined)).toEqual([]);
});

it("changes roles like the server", () => {
  expect(canChangeRole("owner", "owner")).toBe(true);
  expect(canChangeRole("admin", "owner")).toBe(false);
  expect(canChangeRole("admin", "member")).toBe(true);
  expect(canChangeRole("admin", "admin")).toBe(true);
  expect(canChangeRole("member", "viewer")).toBe(false);
  expect(canChangeRole(undefined, "viewer")).toBe(false);
});

it("removes members like the server", () => {
  expect(canRemoveMember("viewer", "viewer", true)).toBe(true);
  expect(canRemoveMember("admin", "owner", false)).toBe(false);
  expect(canRemoveMember("owner", "owner", false)).toBe(true);
  expect(canRemoveMember("admin", "member", false)).toBe(true);
  expect(canRemoveMember("member", "viewer", false)).toBe(false);
});

it("lets only admins and owners manage connections", () => {
  expect(canManageConnections("owner")).toBe(true);
  expect(canManageConnections("admin")).toBe(true);
  expect(canManageConnections("member")).toBe(false);
  expect(canManageConnections("viewer")).toBe(false);
  expect(canManageConnections(undefined)).toBe(false);
});

it("overrides only members and viewers", () => {
  expect(canHaveOverride("member")).toBe(true);
  expect(canHaveOverride("viewer")).toBe(true);
  expect(canHaveOverride("admin")).toBe(false);
  expect(canHaveOverride("owner")).toBe(false);
});

it("lets members and above manage evaluation", () => {
  expect(canManageEvaluation("admin")).toBe(true);
  expect(canManageEvaluation("member")).toBe(true);
  expect(canManageEvaluation("viewer")).toBe(false);
  expect(canManageEvaluation(undefined)).toBe(false);
});
