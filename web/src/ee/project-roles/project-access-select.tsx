// SPDX-License-Identifier: Elastic-2.0
import type { Schema } from "@/api/types";
import { DisabledReason } from "@/components/disabled-reason";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { overrideLabel } from "@/lib/roles";

export const OVERRIDES = ["admin", "member", "viewer", "none"] as const;

type Access = "inherit" | (typeof OVERRIDES)[number];

/**
 * The project access picker for one member. When `locked` (the plan has no
 * `project_roles`), new overrides are disabled but the member's current override
 * can still be removed with "Inherit org role" (D-P-8).
 */
export function ProjectAccessSelect(props: {
  member: Schema<"ProjectMember">;
  canEdit: boolean;
  locked: boolean;
  pending: boolean;
  onChange: (value: Access) => void;
}) {
  const { member, canEdit, locked, pending, onChange } = props;
  const current = member.override ?? "inherit";

  return (
    <DisabledReason reason={canEdit ? undefined : "Only project admins can change access"}>
      <Select
        value={current}
        onValueChange={(value) => {
          const choice = (["inherit", ...OVERRIDES] as const).find((item) => item === value);
          if (choice) {
            onChange(choice);
          }
        }}
        disabled={!canEdit || pending}
      >
        <SelectTrigger aria-label={`Project access of ${member.user.email}`} className="h-7 w-40">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {(["inherit", ...OVERRIDES] as const).map((value) => (
            <SelectItem
              key={value}
              value={value}
              disabled={locked && value !== "inherit" && value !== current}
            >
              {overrideLabel(value)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </DisabledReason>
  );
}
