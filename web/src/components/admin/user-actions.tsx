import { Loader2, MoreHorizontal } from "lucide-react";
import type { Schema } from "@/api/types";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

export function UserActions(props: {
  user: Schema<"AdminUser">;
  self: boolean;
  pending: boolean;
  onToggleAdmin: (user: Schema<"AdminUser">) => void;
  onDisable: (user: Schema<"AdminUser">) => void;
  onEnable: (user: Schema<"AdminUser">) => void;
  onResetLink: (user: Schema<"AdminUser">) => void;
}) {
  const { user, self, pending } = props;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label={`Actions for ${user.email}`}
          disabled={pending}
        >
          {pending ? <Loader2 className="animate-spin" /> : <MoreHorizontal />}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        <DropdownMenuItem disabled={self} onSelect={() => props.onToggleAdmin(user)}>
          {user.is_instance_admin ? "Remove instance admin" : "Make instance admin"}
        </DropdownMenuItem>
        <DropdownMenuItem
          disabled={self}
          onSelect={() => {
            if (user.disabled) {
              props.onEnable(user);
            } else {
              props.onDisable(user);
            }
          }}
        >
          {user.disabled ? "Enable account" : "Disable account"}
        </DropdownMenuItem>
        {self ? (
          <p className="px-2 py-1.5 text-xs text-muted-foreground">
            You can't change your own account
          </p>
        ) : null}
        <DropdownMenuSeparator />
        <DropdownMenuItem disabled={user.disabled} onSelect={() => props.onResetLink(user)}>
          Create reset link
        </DropdownMenuItem>
        {user.disabled ? (
          <p className="px-2 py-1.5 text-xs text-muted-foreground">
            Enable the account to create a reset link
          </p>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
