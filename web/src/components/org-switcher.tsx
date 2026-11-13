import { useNavigate, useParams } from "@tanstack/react-router";
import { Plus } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { NewOrgDialog } from "@/components/settings/new-org-dialog";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

export function OrgSwitcher() {
  const { org: current } = useParams({ from: "/$org" });
  const navigate = useNavigate();
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const membership = me.orgs.find((org) => org.slug === current);
  const [creating, setCreating] = useState(false);

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" aria-label={membership?.name ?? current}>
            {membership?.name ?? current}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start">
          <DropdownMenuRadioGroup
            value={current}
            onValueChange={(slug) => {
              void navigate({ to: "/$org", params: { org: slug } });
            }}
          >
            {me.orgs.map((org) => (
              <DropdownMenuRadioItem key={org.id} value={org.slug}>
                {org.name}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem onSelect={() => setCreating(true)}>
            <Plus />
            New organization
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <NewOrgDialog open={creating} onOpenChange={setCreating} />
    </>
  );
}
