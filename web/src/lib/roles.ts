import type { Schema } from "@/api/types";

export type OrgRole = Schema<"OrgRole">;
export type RoleOverride = Schema<"ProjectRoleOverride">;

/** Highest first. Menus list roles in this order. */
export const ORG_ROLES: readonly OrgRole[] = ["owner", "admin", "member", "viewer"];

const RANK: Record<OrgRole, number> = { viewer: 0, member: 1, admin: 2, owner: 3 };

export function roleRank(role: OrgRole): number {
  return RANK[role];
}

/** False when `role` is undefined (an API key, or no membership). */
export function atLeast(role: OrgRole | undefined, min: OrgRole): boolean {
  return role !== undefined && RANK[role] >= RANK[min];
}

export function roleLabel(role: OrgRole): string {
  return role.charAt(0).toUpperCase() + role.slice(1);
}

/** Roles `actor` may give in an invite or a role change (D139): none above their own, `owner` only by an owner. */
export function grantableRoles(actor: OrgRole | undefined): OrgRole[] {
  if (!atLeast(actor, "admin") || actor === undefined) {
    return [];
  }
  return ORG_ROLES.filter(
    (role) => RANK[role] <= RANK[actor] && (role !== "owner" || actor === "owner"),
  );
}

/** Mirrors `may_set_role` in crates/server/src/routes/members.rs for the target's current role. */
export function canChangeRole(actor: OrgRole | undefined, target: OrgRole): boolean {
  if (!atLeast(actor, "admin")) {
    return false;
  }
  return target !== "owner" || actor === "owner";
}

/** Anyone may leave. Removing someone else mirrors `may_remove_other`. */
export function canRemoveMember(
  actor: OrgRole | undefined,
  target: OrgRole,
  self: boolean,
): boolean {
  if (self) {
    return true;
  }
  if (!atLeast(actor, "admin")) {
    return false;
  }
  return target !== "owner" || actor === "owner";
}

/** Mirrors `OrgPerm::ManageConnections`: org admins and owners (D-I-24). */
export function canManageConnections(role: OrgRole | undefined): boolean {
  return atLeast(role, "admin");
}

/** Mirrors `ProjectPerm::ManageEvaluation`: project members and above (D-I-24). */
export function canManageEvaluation(role: OrgRole | undefined): boolean {
  return atLeast(role, "member");
}

/** Overrides apply only to org members and viewers (D139). */
export function canHaveOverride(orgRole: OrgRole): boolean {
  return orgRole === "member" || orgRole === "viewer";
}

const OVERRIDE_LABELS: Record<RoleOverride | "inherit", string> = {
  inherit: "Inherit org role",
  admin: "Admin",
  member: "Member",
  viewer: "Viewer",
  none: "No access",
};

export function overrideLabel(value: RoleOverride | "inherit"): string {
  return OVERRIDE_LABELS[value];
}
